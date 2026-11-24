# Everywhere

[简体中文](README.md) | **English**

**Find your files, wherever you remember them.** Everywhere (Chinese name: 随手找) is a local macOS file-name and path search app written in Rust. Speed is the priority: search an in-memory index, render visible rows, and load more results as you scroll.

Remember a word in a file name or a project folder? Start there, narrow by type or modification date, then open the file, reveal it in Finder, or copy its full path.

Created by **随性人啊** · [Bilibili](https://space.bilibili.com/8580292) · [Releases](https://github.com/gkgy/suishou-find/releases) · [Product slides in Chinese](docs/intro.html)

## 0.5.7 startup cache

Full-disk search and date sorting use the existing 0.5.6 cache. Missing entries are repaired in batches of up to 2,048. The first upgraded launch uses the saved dates. After a clean quit, later launches replay macOS file events to refresh dates for indexed paths. If the system journal has a gap, use Index → Rebuild Index for a complete refresh.

## 0.5.6

- **Chinese and English UI:** choose **Settings → 语言 / Language → 中文 / English**. The change takes effect immediately and is saved for the next launch. Main controls, shortcut settings, search locations, date ranges, quick filters, and feedback are translated. File names, paths, and your custom filter names remain unchanged. Native file dialogs follow macOS's language.
- **Continuous scrolling:** the first batch contains up to 500 results. Scroll down to see later results without a Next Page button. The scroll range covers all matches; 500 is a batch size, not a total limit.
- **Bounded rendering and caching:** only visible rows are drawn. The UI and background worker each cache at most eight result windows. Jumping far down requests that window directly. Thumbnails load in the background for visible images.
- **All Locations by default:** choose **My Search Locations…** and check the folders you want, then apply. This filters the existing index; it does not automatically index a new folder.
- **Quick Filters:** use built-in searches, create a combination of criteria, save the current filter, or manage your saved filters. Removing a filter does not delete files.

Download the [v0.5.7 release for Apple Silicon](https://github.com/gkgy/suishou-find/releases/tag/v0.5.7) or build from source below.

## Features

- Search file and folder **names** by default. Enable **Match Paths** to include parent folder names. ASCII English matching is case-insensitive; Chinese keywords are supported.
- Combine up to eight distinct keywords with spaces. All keywords must match. Plain keywords match names; `/folder` keywords include ancestor folder names. For example, `baby /output` finds names containing `baby` under a matching `output` folder. This is not the full Everything query language.
- Filter by All, Folders, Documents, Images, Audio, Video, Applications, Archives, Code, or Other. Categories use extensions and directory flags, without reading file contents.
- Sort by Relevance, Newest First, Oldest First, or Name A–Z. Sort order covers all matches. Click the Date Modified heading to toggle newest/oldest; click Name for name order. Missing dates sort last.
- Filter by modification date: Past 3 Days, Past Week, Past Month (30 days), Past Year (365 days), or custom dates. These are modification dates, not last-opened dates. Custom dates use local time and include the end date.
- Switch between **List** and **Thumbnails**, with an optional preview panel. PNG, JPEG, WebP, and GIF previews retain their aspect ratio; unsupported or oversized images show a placeholder.
- Index selected folders or local disks, including locally attached external drives. Watch indexed locations and update changes in the background.
- Show or hide the running app with **⌥ Space** by default. Customize it in **Keyboard Shortcut**. Choose at least one of Command, Option, or Control; function keys may require Fn. The shortcut does not launch an app that is not running.
- Click to select, double-click or press Enter to open, use arrow keys to navigate, or right-click to reveal in Finder/copy the full path.
- Everything runs locally, without an account, cloud service, or AI model.

Relevance favors exact names, then prefixes, then other substring matches. In the Applications category, standalone installed `.app` bundles are favored over helpers and temporary copies. This uses name and path rules, not semantic ranking. Date sorting follows dates strictly.

## First run

1. Download a macOS build from [Releases](https://github.com/gkgy/suishou-find/releases), or build the current source.
2. Extract and move **随手找.app** to Applications. Its English display name is **Everywhere**; the bundle filename is retained for compatibility.
3. Open **Index → Choose Locations…** and select a frequently used folder.
4. Wait for the initial scan, enter a keyword, and narrow the results if needed.
5. Select **Settings → Language → English** for the English interface.

Builds target Apple Silicon / arm64. The declared minimum is macOS 13; local acceptance was performed on an M5 Max Mac Studio. Intel Macs and other macOS versions have not been accepted locally. Builds use ad-hoc signing unless you supply your own signing identity; they are not Apple-notarized. Protected locations depend on macOS access permissions.

## Examples

| What you remember | Search and filter | Action |
|---|---|---|
| A document title | `report`, Documents, Past Week | Open the matching report |
| A project folder | `/projectname`, Video | Find video files under that project |
| Generated pictures | `/output`, Images, Newest First, Thumbnails | Preview recent outputs |
| A PDF name | `manual .pdf`, Documents | Open the manual |
| A file needed by another tool | Search its name | Copy Full Path |

Results depend on what has been indexed. An empty query with no filter does not list the entire index. Search does not read document text, perform OCR, or provide semantic, pinyin, or fuzzy matching. Path matching works with individual name components; a literal cross-separator string such as `foo/bar` is not guaranteed to match. Use separate folder keywords such as `/foo /bar`.

Explicitly selected search locations can override exclusion rules. Default exclusions cover hidden folders, app bundles, Library, and common dependency/build folders such as `node_modules`, `target`, `venv`, `__pycache__`, `dist`, and `build`. Edit them under **My Search Locations → Advanced: Exclusions**.

## Performance and validation

The 0.5.5 continuous-scroll change preserved the query engine. On a fixed index of **2,270,113 items**, after three warm-up runs and 21 measured runs, first-batch median query times were 3.208 ms (`output`), 3.041 ms (`pdf`), 9.489 ms (`md`), and 2.678 ms (`SCADA`). Broad matches took longer: 58.647 ms (`a`) and 82.181 ms (`.`).

These measurements cover in-memory matching, filtering, and sorting, not startup, a complete UI frame, file synchronization, image decoding, or opening files. They are local observations, not guarantees for every query or computer. Language selection changes presentation and does not alter query semantics or the index.

Native acceptance for continuous scrolling included crossing result 500, reaching all 2,231 results in an example search, resetting to the top for a new query, and selecting/previewing/opening an image beyond result 500. Historical measurements and previous-version details are retained in the [Chinese README](README.md) and [changelog](CHANGELOG.md); older 500-result display limits do not apply to the current source.

## Build from source

Requires macOS, a Rust toolchain supporting edition 2024, and Xcode Command Line Tools. Dependencies are pinned in `Cargo.lock`.

```bash
git clone https://github.com/gkgy/suishou-find.git
cd suishou-find
cargo test --locked --lib --bin miao-find
./scripts/build_app.sh
open dist/随手找.app
```

To use your own Apple signing identity:

```bash
SIGN_IDENTITY="Your signing identity" ./scripts/build_app.sh
```

To benchmark your own index snapshot:

```bash
cargo run --locked --release --bin bench-latency -- /path/to/index-v3.bin /path/to/modified-v1.bin
```

The repository does not include the author's private index or file listing.

## Local data and compatibility

The internal bundle identifier remains `com.gkgy.MiaoFind`, the executable is `MiaoFind`, and the Cargo package is `miao-find`. Keeping these identifiers preserves existing indexes and preferences while using the Everywhere brand.

The index format remains MIAOIDX3, with a separate `modified-v1.bin` date cache. Index files live in `~/Library/Application Support/com.gkgy.MiaoFind/`; preferences live in `~/Library/Application Support/随手找/`. Upgrading does not require rebuilding an existing compatible index. Missing dates are populated in the background, and changes while the app was closed are reconciled after startup.

Indexes contain file names and folder structure. Access depends on macOS permissions; inaccessible locations cannot be fully indexed. Network mounts can be selected manually, but scanning and monitoring behavior depend on the filesystem and have not been guaranteed.

## About and license

Everywhere is a small local tool by **随性人啊**, built to reduce time spent digging through folders. Visit [Bilibili](https://space.bilibili.com/8580292) or submit feedback through [Issues](https://github.com/gkgy/suishou-find/issues). Include your OS version, reproduction steps, and expected behavior; omit private paths and index contents.

[MIT License](LICENSE).

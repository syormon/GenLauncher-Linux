# GenLauncher (Rust / egui)

A port of the WPF `GenLauncherNet` project to Rust with [egui](https://github.com/emilk/egui).

It is a drop-in replacement for the existing launcher: same on-disk layout, same
config file, same repository manifests. Copy the binary into a Generals or
Zero Hour folder alongside (or instead of) `GenLauncher.exe` and existing
installs carry over untouched.

## Standalone

The binary carries everything it needs: the 11 translations, the placeholder mod
artwork, the stock `Options.ini` and `d3d8.cfg`, and the window icon are all
compiled in. On Windows the MSVC runtime is linked statically (see
`.cargo/config.toml`), so the only imports are system DLLs — no .NET runtime, no
side-by-side DLLs, nothing from the old C# build.

It does not download or install GenTool. The version check is ported in
`game::gentool` but, as in the C# build, nothing calls it; the only GenTool code
that runs edits the local `d3d8.cfg` or moves `d3d8.dll` aside when you turn the
option off.

Network use at runtime, all to the project's own published data:

| When | What |
| --- | --- |
| Start-up | the repository index and the manifests/logos of mods you already have |
| Adding a mod | that mod's manifest and its patch/addon manifests |
| Installing | the mod's files, from its S3 bucket or its share link |
| Installing (S3 only) | one SNTP query to `time.windows.com`, to warn about a skewed clock |
| Launching with Vulkan on | the Vulkan layer, if the repository has a newer one |
| Launcher update | only when you press Update |

The one optional external tool is a RAR extractor (`7z`, `unar` or `unrar` on
PATH), needed only if you add a mod from a `.rar` by hand. zip and 7z are
handled in-process.

## Build and run

```bash
cargo run --release
```

The launcher operates on its working directory, so run it from the game folder.

## Releases

`.github/workflows/release.yml` builds Linux and Windows on every push to
`master`/`main`, and publishes a release when the version is new.

The release number is the `version` in **Cargo.toml**, and nothing else:
`config::VERSION` reads it through `CARGO_PKG_VERSION`, so the binary, the
"Version:" line in the UI and the git tag can never drift apart. A test asserts
that link.

To cut a release, bump `version` in `Cargo.toml` and push. The workflow:

1. reads the version and derives the tag `v<version>`;
2. skips publishing if that release already exists, so ordinary pushes just
   build and test;
3. runs clippy (`-D warnings`) and the test suite on both platforms;
4. re-checks the release is still absent, then creates it with both archives
   and their SHA-256 sums.

Assets are `GenLauncher-<version>-linux-x86_64.tar.gz` and
`GenLauncher-<version>-windows-x86_64.zip`, each containing a `GenLauncher/`
folder. `workflow_dispatch` offers a `publish` toggle for a build-only run.

## Layout

| Module | Replaces |
| --- | --- |
| `config` | `EntryPoint` constants, game-folder resolution |
| `model` | `LauncherData`, `GameModification`, `ModificationVersion`, `ColorsInfo` |
| `state` | `DataHandler` |
| `net::manifests` | `GitHubYamlReader`, `GitHubMainDataReader` |
| `net::s3` | `S3StorageHandler` (MinIO listing, now via SigV4 presigning) |
| `net::download` | `HttpSingleFileUpdater`, `S3Updater`, `ContentDownloader` |
| `game::launcher` | `GameLauncher`, `FilesHandler`, `FoldersHandler` |
| `game::symlinks` | `SymbolicLinkHandler` |
| `game::big` | `BigHandler` |
| `game::options` | `GameOptionsHandler` |
| `game::gentool`, `game::vulkan` | `GentoolHandler`, `VulkanDllsHandler` |
| `util::pe_version` | the `version.dll` P/Invoke and `FileVersionInfo` |
| `tasks` | the `async void` / `Task.Run` work in `MainWindow` and `InitWindow` |
| `ui::*` | `MainWindow.xaml`, `OptionsWindow.xaml`, `InfoWindow`, `AddModificationWindow`, ... |

Threading model: the UI never blocks. Background work runs on a Tokio runtime
(or a plain thread for the blocking game launch) and reports back over an
`mpsc` channel that `GenLauncherApp::pump_messages` drains once per frame.

## Notable differences from the C# build

* **YAML scalars are coerced to strings.** `Version: 1.86` is a float in YAML.
  YamlDotNet silently converted it; a strict Rust deserializer would reject the
  file, so `model::null_to_empty` reproduces the coercion. Numeric versions are
  written back quoted, so they round-trip.
* **Windows chrome replaced with a normal decorated window.** The original drew
  its own title bar and implemented dragging by hand.
* **Flat background instead of the painted backdrop.** The WPF build stretched
  `Background.png` across a fixed 986x762 window; the frame graphics baked into
  that image only lined up with its exact grid. The layout is now fluid and the
  window resizes, so the artwork is gone and panels are drawn in the palette
  instead. A mod's `ColorsInformation` palette is still applied in full — only
  its `GenLauncherBackgroundImageLink` is unused.
* **Mod cards are denser**, with the banner scaled to the panel and capped in
  height so more of the list fits on screen.
* **Single-instance guard is a lock file** rather than a named mutex, so it
  works the same way on Linux.
* **Grayscale mod artwork is generated in memory** instead of writing a second
  `*BW` file next to each logo.
* **S3 listings go over plain HTTP.** Manifests give a bare `host:port` such as
  `gen.insave.ovh:9000`. MinIO's .NET client defaults to non-SSL and the C#
  build never called `WithSSL()`, and the storage only answers HTTP on that
  port — so http is tried first, https second. Keys come back percent-encoded
  (`encoding-type=url`) and are decoded before use, because mod files are named
  things like `!!OFS_ALPHA_Window_16_9.big`.
* **RAR extraction shells out** to `7z`, `unar` or `unrar` if one is on PATH.
  zip and 7z are handled in-process.
* **No self-update.** The C# build fetched whatever `DownloadLink` the upstream
  manifest advertised, which is the C# launcher's own zip — this build would
  have replaced itself with it. The feature is gone; releases come from this
  repository instead.
* **No advertising card.** The upstream launcher injected a promotional entry at
  the top of the mod list and persisted it in the config. It is not shown, and
  an entry left behind by the C# launcher is dropped on load.
* **Dead code kept dead.** `CheckAndUpdateGentool`, `CheckModdedExe` and
  `CheckAndDowloadWB` were defined but never called in the C# build; the logic
  is ported (see `game::gentool`) but stays uncalled so behaviour matches.
  Wiring the GenTool auto-update back up is a one-line change if wanted.

## Linux support

Not done yet — this stage is the migration only. The groundwork is in place:
all path handling is `PathBuf`-based, symlinks go through `util::fs`, the
version-resource reader is pure Rust rather than `version.dll`, and
`game::launcher::spawn_exe` already falls back to Wine for a `.exe`. What still
needs doing is prefix/path mapping, the user-data directory under a Wine
prefix, and case-insensitive game-file matching.

## Tests

```bash
cargo test
```

Covers version ordering, the C#-compatible config format, S3 listing parsing,
download filename handling, the `.gib`/`.big` equivalence rule, manifest
parsing, drag-and-drop reordering and the localization fallbacks.

Two tests hit the real storage and are opt-in:

```bash
cargo test -- --ignored
```

They check that the S3 endpoint is reachable, that object keys arrive decoded,
and that a real file downloads and lands under its literal name.

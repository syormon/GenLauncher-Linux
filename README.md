# GenLauncher-Linux

This is a based off of [GenLauncher](https://github.com/p0ls3r/GenLauncher). Tested on Windows 10 and Ubuntu.

# Why migrate this to Rust?
1. The original is writen in `.NET Framework`, which generally isn't suppport on Linux
2. I comtemplated migrated `.NET Framwork` to `.NET Core`, but `.NET Core` isn't support on BSD, which is a goal
3. A Systems language is a better language for this kind of tool, as the code base dropped from ~100k loc to ~20k loc.
4. I have limited time to support this and LLMs are better at rust

# Install

1. Grab a binary from the [releases](https://github.com/syormon/genlauncher-linux/releases)
2. Move the binary into the Generals folder
3. run executable (Linux users may need to `chmod +x` binary)

> You don't have to launch the vanilla game first, as `GenLauncher-Linux` will create the `options.ini` file if it doesn't exist.

> If you're having issues running the game, check [protondb](https://www.protondb.com/app/2732960) and configure the compatability mode of the game to use the newest version of proton. I used `experimental` on ubuntu26.

## Differences from the original

* lock file rather than a mutex, so it works the same on Windows/Linux.
* extraction attempts `7z`, `unar` or `unrar` if one is on PATH.
* No self-update
* No advertising card
* **Modded executable.** `Use modded exe files` [default] now downloads the repository's `ModdedExe` and runs it.
* With `Install and autoupdate Gentool` on [default], each game launch checks
gentool.net and installs or updates GenTool's `d3d8.dll` in the game folder.
With it off, `d3d8.dll` is moved aside for the run. 

### Known issues and their workarounds

- Steam edition sometimes gets error "Failed to fetch Steam App Name!". Make sure `Use modded exe files` is on.

- With more than one monitor, the game can ignore the mouse. This is due to how Wine runs the virtual desktop

- If the game is running, GenLauncher will refuse to close by design to prevent any race condition from the lock file.

## Releases

> bumping `version` in `Cargo.toml` triggers the `release.yml` CI

## Build and run

```bash
cargo run --release
```

## Tests

```bash
cargo test
```

On Linux, one more drives a whole launch through a stand-in `wine` script: mod
linking into differently-cased folders, the environment Wine receives, waiting
for `game.dat`, and restoring the folder. It claims the process-wide game
folder, so run it on its own (CI does):

```bash
cargo test proton_launch_end_to_end -- --ignored
```

## General Network usage:

| Action | What |
| --- | --- |
| Start-up | the repository index and the manifests/logos of mods you already have |
| Adding a mod | that mod's manifest and its patch/addon manifests |
| Installing | the mod's files, from its S3 bucket or its share link |
| Installing (S3 only) | one SNTP query to `time.windows.com`, to warn about a skewed clock |
| Launching with GenTool on | the gentool.net front page, and GenTool itself when it is missing or outdated |
| Launching Zero Hour with "Use modded exe files" on | the repository's modded executable, once |
| Launching with Vulkan on | the Vulkan layer, if the repository has a newer one |

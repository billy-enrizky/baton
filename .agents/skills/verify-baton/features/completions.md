# Shell completions

## Sub-features
- `baton completions bash|zsh|fish|powershell|elvish` prints a completion script.
- The script is generated from the same clap command graph the binary parses.
- Broken-pipe output from `| head` exits cleanly instead of panicking.

## How to get to it (user POV)
```sh
baton completions bash
baton completions zsh
baton completions fish
baton completions powershell
baton completions elvish
```

## Driving it with the unit harness
```sh
cargo test --locked --lib completions_accept_every_supported_shell
```

The test parses every supported shell and generates a non-empty Bash script.
A separate runtime check should pipe a script through `head` and require exit 0.

## Gotchas
- The command writes only the script to stdout. Redirect it to the shell's completion directory.
- `clap_complete` panics when its writer is a broken pipe, so the binary writes into a buffer and handles `BrokenPipe` itself.

# Model catalog

## Sub-features
- `/model` merges three sources in order: the adapter's live listing, the last full listing Baton recorded for that harness, and the static known catalog.
- A live probe that returns fewer models than the recorded list does not replace it, so a degraded probe cannot shrink the picker.
- A failed, timed-out, or busy probe opens the picker with the recorded and known models and a notice that names the error.
- The recorded list is a private file under `~/.baton/model-cache/`, written atomically and read only when it is a regular file at or below 64 KiB.

## How to get to it (user POV)
Open `/model` or start `/switch` and choose a harness. The picker shows the merged list and the notice says whether the listing was live, cached, or both.

## Unit proof
```sh
cargo test --locked --lib -- model_cache merge_catalogs short_live_catalog failed_live_probe
```

- `merge_catalogs_keeps_live_then_cached_then_known` proves the order and dedupe.
- `model_cache_round_trips_and_rejects_unsafe_files` proves trimming, control-character rejection, dedupe, the 64 KiB cap, and that a symlink is ignored.
- `model_cache_files_are_private` proves the directory is 0700 and the file is 0600.
- `a_short_live_catalog_does_not_shrink_the_cache` proves a one-model probe keeps a newer recorded model that is not in the static catalog.
- `a_failed_live_probe_still_offers_the_cached_and_known_models` proves the error path still opens a usable picker.

## Gotchas
- The cache is keyed only by provider. Named profiles and custom endpoints will need a profile key before their catalogs can differ.
- The recorded list has no expiry. It is replaced only by a listing at least as long, so a provider that genuinely removes models keeps the removed ones until a full listing replaces the file. This is deliberate: a stale extra row is a smaller failure than a missing model.
- The file is read on `/model`, not at startup, so it never delays the first screen.
- Cache write failures are ignored; the picker still works from the live and static lists.

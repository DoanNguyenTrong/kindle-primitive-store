# Primitive Store

The app store for jailbroken Kindle e-readers, as its own program. It shows the catalog (Today, Apps, Updates), installs,
updates and removes apps, and is opened from the Kindle's library (`Store.sh`), like Primitive Browser.

It has its own repository and its own deploy: changing the store does not require a launcher build.

- `crates/store-app`: the program (the screens, the event loop, the store's network and install work).
- `tools/`: the shared kit (screen layout and renderer, the Kindle device layer, the store engine, the network code).
- `deploy/`: `Store.sh` (the library entry) and `install-ssh.sh` (installs the program and the entry over SSH).

## Build and install

```sh
git submodule update --init --recursive
cargo zigbuild --release -p store-app --target armv7-unknown-linux-musleabihf
sh deploy/install-ssh.sh target/armv7-unknown-linux-musleabihf/release/store-app
```

The store keeps its catalog, installed apps and records in the launcher's folder (`/mnt/us/launcher/store`).

## Licence

Code: PolyForm Noncommercial 1.0.0. The shared kit in `tools/` is AGPL-3.0 or PolyForm Noncommercial 1.0.0.

# Changelog

## 0.1.8

- Follow Primitive Launcher's system layer. Store.sh declares `# Wake: redraw`. When the launcher asks, Store reads the
  theme again and redraws its whole screen without a flash.
- Under the launcher, leave the swipe down from the top edge to the launcher's quick settings. Do not redraw again on
  wake, as the launcher has already put the screen back.

## 0.1.7

- Refresh from the PrimitiveOS organization’s Store catalog.

## 0.1.6

- Swipe vertically through Store lists. The fixed tab bar leaves more room for app rows.
- Restore the list position when returning from a product page. Preload icons around the visible rows.
- Show GitHub package download totals on app pages from the saved catalog.

## 0.1.5

- Open the Updates tab and installed-app list from Launcher Settings.
- Defer automatic catalog checks in Launcher Low power mode. Manual Refresh remains available.

## 0.1.4

- Show the cached catalog immediately and preload icons for nearby pages. Decode screenshots when opening an app page.
- Reuse unchanged catalog entries and check changed listings in parallel. Keep completed listings available when images fail.
- Simplify refresh progress and keep Done available during a refresh.

## 0.1.3

- Show catalog refresh progress and let Done stop a refresh safely before returning to Launcher.
- Reuse the network connection for catalog requests and set a time limit on the refresh.

## 0.1.2

- Load the catalog on Kindle firmware that keeps the optional signing-policy directory group-writable. A configured signing key still requires a protected path.

## 0.1.1

- Display the standalone app as Store in the Kindle library and Launcher grid.

## 0.1.0

- Search the catalog by title, description line, developer or category.
- Review an install before starting it and see when the download, checks and install finish.
- Review and approve missing required apps before the store installs them in order.
- Browse collections, categories, screenshots and release notes. Update several apps at once or clear the saved catalog.
- The store follows sleep, wake and rotation, and refreshes an old catalog automatically when Wi-Fi is connected.
- Stage a launcher update for its supervisor so it can restore the old version after several failed starts.
- The store is its own program: opened from the library (`Store.sh`), with the same screens as before (Today, Apps,
  Updates, an app's page), Refresh, Get, Update, Remove, and the page buttons.
- The shared kit (screen, renderer, device layer, store engine) comes from the tools repository.

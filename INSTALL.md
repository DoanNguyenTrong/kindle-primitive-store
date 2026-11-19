# Install Store

Install Primitive Launcher first. The Store uses the launcher's fonts and settings, and saves its catalog and installed
apps in `launcher/store/`. This package is built for hard-float Kindles with firmware 5.16.3 or later.

## Install from the launcher

When Store is available from the launcher's Store tile, open the tile and follow the installation prompt.
Open Store from the launcher after installation.

## Install over USB

1. Verify the package against its accompanying SHA-256 file and extract the zip on your computer.
2. Connect the Kindle as a USB drive. Copy the extracted top folder's `store/` and `documents/` folders onto the
   Kindle drive, merging with the existing folders.
3. Eject the Kindle, refresh the launcher's library if needed, and open **Store**. Tap **Done** to return to
   the launcher.

Use the same procedure to replace a manual installation, with Store closed. Keep `launcher/store/` to preserve
the catalog and installed-app records.

The repository also provides `deploy/install-ssh.sh` for an authorized maintainer who already has SSH access to the Kindle.

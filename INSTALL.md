# Install Store

Store is part of PrimitiveOS. It uses Primitive Launcher's fonts and settings, and saves its catalog and install
records in `launcher/store/`. This package is built for hard-float Kindles with firmware 5.16.3 or later.

## Install from Launcher

Complete the [first-time PrimitiveOS setup](https://github.com/DoanNguyenTrong/kindle-primitive-launcher/blob/main/docs/INSTALL.md#first-time-setup-over-usb)
with Primitive Launcher. Open the **Store** tile and follow the installation prompt, then use **Refresh** to get
the catalog.

## Test a Store package over USB

1. Verify the package against its accompanying SHA-256 file and extract the zip on your computer.
2. Connect the Kindle as a USB drive. Copy the extracted top folder's `store/` and `documents/` folders onto the
   Kindle drive, merging with the existing folders.
3. Eject the Kindle, refresh the launcher's library if needed, and open **Store**. Tap **Done** to return to
   the launcher.

Close Store before replacing a test build. Keep `launcher/store/` to preserve the catalog and installed-app records.

The repository also provides `deploy/install-ssh.sh` for an authorized maintainer who already has SSH access to the Kindle.

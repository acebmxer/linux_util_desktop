# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Each release records
the linux_util version it bundles in `vendor/linux_util.VERSION`. Add new entries
under **[Unreleased]** as work lands; move them into a versioned section when a
release is cut.

## [Unreleased]

### Added

- **Packages**: an AppImage for any distro, a `.deb` for Debian/Ubuntu and an
  `.rpm` for Fedora, the RHEL family and openSUSE, each released with
  `SHA256SUMS`. The `.deb` and `.rpm` pull in WebKitGTK, GTK, `bash` and `sudo`.
  The menu entry is listed under System.

  Under the AppImage, linux_util.sh and everything it starts get the system's
  own libraries and paths, not the AppImage's, and run from the home directory.
  Without that, commands such as `curl`, `git` and `pacman` failed with
  "symbol lookup error".
- **Install, uninstall and update** one or more selected utilities, one at a
  time, with live output in a log pane, the result of each, a dry-run toggle,
  cancel, and a copy-log button. The administrator password is asked for in the
  app's own dialog, checked once and kept in memory only for that run.

  For System Tasks, which change a setting rather than add software, the
  buttons read **Apply** and **Revert** and the status badge reads **Applied**.
- **Utility list** read from the bundled linux_util (v1.9.0): every utility with
  its installed status and version, a category sidebar with sub-category
  headings, a search box, and the detected distro in the header. Follows the
  system's light or dark theme.

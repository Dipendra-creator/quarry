Native database client, SQL editor and AI assistant, with PostgreSQL, MySQL, MariaDB and SQLite support. MIT licensed.

This release adds native builds for all six desktop targets:

| Platform | Architectures | Downloads |
| --- | --- | --- |
| Windows | x64, ARM64 | Installer EXE and portable ZIP |
| macOS | Intel x64, Apple silicon ARM64 | DMG and ZIP containing Quarry.app |
| Linux | x64, ARM64 | Debian package and portable tar.gz |

Every target runs the workspace tests, verifies the executable architecture and starts the MCP server before packaging. SHA256SUMS.txt covers all 12 downloads.

**Installing**

- Windows x64: Windows 10 1809 or later; Windows ARM64: Windows 11. Run the matching installer.
- macOS: deployment target macOS 12 or later; CI runs on macOS 15. Open the DMG and drag Quarry to Applications. The app is **ad hoc signed, not Apple notarized**. If macOS blocks a trusted download, use System Settings → Privacy & Security → Open Anyway.
- Linux: built on Ubuntu 22.04 (glibc 2.35 baseline). Install the matching DEB with apt, or extract the tarball on a compatible system with the required graphics libraries. See the README for dependencies.

The MCP server is included in every package. On macOS it is inside Quarry.app/Contents/MacOS; the app's Preferences shows the exact registration command. AI backends use your CLI login or API credentials; provider charges may apply. Server backup and restore also need the corresponding database tools on PATH.
